use deku::ctx::Endian;
use deku::prelude::*;

pub const SOM: u16 = 0x4040;

#[derive(DekuRead, DekuWrite, Debug, Clone, Copy, PartialEq, Eq)]
#[deku(id_type = "u16", ctx = "endian: Endian", endian = "endian")]
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
#[deku(ctx = "endian: Endian", endian = "endian")]
pub struct ParameterId(pub u16);

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

    pub fn raw(self) -> u16 {
        self.0
    }
}

#[derive(DekuRead, DekuWrite, Debug, Clone, PartialEq)]
#[deku(ctx = "endian: Endian", endian = "endian")]
pub struct Parameter {
    #[deku(update = "4 + self.data.len() as u16")]
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
        let trimmed = data.iter().take_while(|&&b| b != 0).copied().collect();
        String::from_utf8(trimmed).ok()
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

#[derive(DekuRead, DekuWrite, Debug, Clone, PartialEq)]
#[deku(ctx = "endian: Endian", ctx_default = "Endian::Big", endian = "endian")]
pub struct ControlMessageSet {
    #[deku(read_all)]
    pub messages: Vec<ControlMessage>,
}

#[derive(DekuRead, DekuWrite, Debug, Clone, PartialEq)]
#[deku(ctx = "endian: Endian", ctx_default = "Endian::Big", endian = "endian")]
pub struct ControlMessage {
    pub som: u16,
    #[deku(
        update = "6 + self.parameters.iter().map(|p| p.total_length as usize).sum::<usize>() as u16"
    )]
    pub total_length: u16,
    pub message_id: ControlMessageId,
    #[deku(bytes_read = "total_length.saturating_sub(6)")]
    pub parameters: Vec<Parameter>,
}

impl ControlMessage {
    pub fn new(message_id: ControlMessageId, parameters: Vec<Parameter>) -> Self {
        let total_length = 6 + parameters
            .iter()
            .map(|p| p.total_length as usize)
            .sum::<usize>() as u16;
        ControlMessage {
            som: SOM,
            total_length,
            message_id,
            parameters,
        }
    }

    /// StartIdentification carries no parameters - it is purely the device telling the
    /// accessory to send its IdentificationInformation. An iPhone's is 6 bytes on the
    /// wire (`4040 0006 1d00`); the identity fields belong to the accessory's reply.
    pub fn start_identification() -> Self {
        Self::new(ControlMessageId::StartIdentification, vec![])
    }

    pub fn identification_accepted() -> Self {
        Self::new(ControlMessageId::IdentificationAccepted, vec![])
    }

    pub fn request_auth_certificate() -> Self {
        Self::new(ControlMessageId::RequestAuthenticationCertificate, vec![])
    }

    pub fn request_auth_certificate_serial() -> Self {
        Self::new(
            ControlMessageId::RequestAuthenticationCertificate,
            vec![Parameter::new(ParameterId(0x0000), &[][..])],
        )
    }

    pub fn authentication_challenge(challenge: &[u8]) -> Self {
        Self::new(
            ControlMessageId::RequestAuthenticationChallengeResponse,
            vec![Parameter::new(ParameterId(0x0000), challenge)],
        )
    }

    pub fn authentication_succeeded() -> Self {
        Self::new(ControlMessageId::AuthenticationSucceeded, vec![])
    }

    pub fn wireless_carplay_update() -> Self {
        Self::new(
            ControlMessageId::WirelessCarPlayUpdate,
            vec![
                Parameter::new(ParameterId(0x0001), 0x00000001u32),
                Parameter::new(ParameterId(0x0002), 0x01u8),
            ],
        )
    }

    pub fn now_playing_update() -> Self {
        Self::new(ControlMessageId::NowPlayingUpdate, vec![])
    }

    pub fn request_accessory_wifi_config() -> Self {
        Self::new(
            ControlMessageId::RequestAccessoryWiFiConfigurationInformation,
            vec![],
        )
    }

    pub fn wifi_information(ssid: &str, passphrase: &str) -> Self {
        Self::new(
            ControlMessageId::WiFiInformation,
            vec![
                Parameter::new(ParameterId::SSID, ssid),
                Parameter::new(ParameterId::PASSPHRASE, passphrase),
            ],
        )
    }

    pub fn start_ea_session(protocol: &str) -> Self {
        Self::new(
            ControlMessageId::StartExternalAccessoryProtocolSession,
            vec![Parameter::new(ParameterId::PROTOCOL_NAME, protocol)],
        )
    }

    pub fn ea_session_status(session_id: u8, status: u8) -> Self {
        Self::new(
            ControlMessageId::StatusExternalAccessoryProtocolSession,
            vec![
                Parameter::new(ParameterId::SESSION_IDENTIFIER, session_id as u16),
                Parameter::new(ParameterId::STATUS, status),
            ],
        )
    }
}
