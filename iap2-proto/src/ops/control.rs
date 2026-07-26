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
    // Two dialects of the same exchange are in the wild, differing in *both* opcode and
    // payload shape, so carjack has to speak whichever the accessory declares (see
    // `MESSAGES_RECEIVED_FROM_DEVICE`):
    //   - 0x5702/0x5703, parameters flat (SSID=1, passphrase=2). The AUTO BOX bench unit
    //     answers this one; `carjack.log` 2026-07-17 has it returning `carplay_wifi_9900`.
    //   - 0x4300/0x4301, parameters nested in groups. The CFMOTO bike accepts *only* this
    //     one, and declares it in its IdentificationInformation (`iphone-cfmoto.pklg`).
    #[deku(id = "0x5702")]
    RequestAccessoryWiFiConfigurationInformation,
    #[deku(id = "0x5703")]
    AccessoryWiFiConfigurationInformation,
    #[deku(id = "0x4300")]
    RequestAccessoryWiFiConfigurationInformationGrouped,
    #[deku(id = "0x4301")]
    AccessoryWiFiConfigurationInformationGrouped,
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
    pub const AUTH_DATA: Self = Self(0x0000);
    pub const ACCESSORY_NAME: Self = Self(0x0000);
    /// `IdentificationInformation` parameter 7: the message IDs the accessory will accept
    /// from us. Head units differ on which WiFi-config dialect they implement, so this is
    /// what carjack keys off rather than guessing.
    pub const MESSAGES_RECEIVED_FROM_DEVICE: Self = Self(0x0007);
    pub const DEVICE_NAME: Self = Self(0x0000);
    pub const REQUEST_TRANSPORT_GROUP: Self = Self(0x0000);
    pub const REQUEST_WIFI_GROUP: Self = Self(0x0001);
    pub const TRANSPORT_KIND: Self = Self(0x0000);
    pub const TRANSPORT_IDENTIFIER: Self = Self(0x0001);
    pub const WIFI_CONFIG_GROUP: Self = Self(0x0001);
    pub const WIFI_CONFIG_SSID: Self = Self(0x0000);
    pub const WIFI_CONFIG_PASSPHRASE: Self = Self(0x0001);
    pub const WIFI_CONFIG_CHANNEL: Self = Self(0x0002);
    pub const WIFI_CONFIG_IPV6: Self = Self(0x0003);
    pub const WIFI_CONFIG_PORT: Self = Self(0x0002);
    pub const WIFI_CONFIG_MAC: Self = Self(0x0003);
    pub const WIFI_CONFIG_UUID: Self = Self(0x0004);
    pub const WIFI_CONFIG_VERSION: Self = Self(0x0005);

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
        let mut data = self.as_bytes().to_vec();
        data.push(0);
        Parameter {
            total_length: 0,
            id,
            data,
        }
    }
}

impl IntoParameter for String {
    fn into_parameter(self, id: ParameterId) -> Parameter {
        self.as_str().into_parameter(id)
    }
}

/// A parameter whose value is itself a list of parameters (an iAP2 "group").
impl IntoParameter for Vec<Parameter> {
    fn into_parameter(self, id: ParameterId) -> Parameter {
        let mut data = Vec::new();
        for child in &self {
            data.extend_from_slice(&child.total_length.to_be_bytes());
            data.extend_from_slice(&child.id.0.to_be_bytes());
            data.extend_from_slice(&child.data);
        }
        Parameter {
            total_length: 0,
            id,
            data,
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

impl TryFromParam for u8 {
    fn try_from_param(data: &[u8]) -> Option<Self> {
        data.try_into().ok().map(u8::from_be_bytes)
    }
}

impl TryFromParam for u16 {
    fn try_from_param(data: &[u8]) -> Option<Self> {
        data.try_into().ok().map(u16::from_be_bytes)
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

/// A packed big-endian list of u16s, as used by the message-ID lists in
/// `IdentificationInformation` (parameters 6 and 7).
impl TryFromParam for Vec<u16> {
    fn try_from_param(data: &[u8]) -> Option<Self> {
        if data.is_empty() || data.len() % 2 != 0 {
            return None;
        }
        Some(
            data.chunks_exact(2)
                .map(|c| u16::from_be_bytes([c[0], c[1]]))
                .collect(),
        )
    }
}

impl TryFromParam for Vec<Parameter> {
    fn try_from_param(data: &[u8]) -> Option<Self> {
        let mut out = Vec::new();
        let mut rest = data;
        while !rest.is_empty() {
            if rest.len() < 4 {
                return None;
            }
            let total_length = u16::from_be_bytes([rest[0], rest[1]]);
            let id = u16::from_be_bytes([rest[2], rest[3]]);
            let len = total_length as usize;
            if len < 4 || len > rest.len() {
                return None;
            }
            out.push(Parameter {
                total_length,
                id: ParameterId(id),
                data: rest[4..len].to_vec(),
            });
            rest = &rest[len..];
        }
        (!out.is_empty()).then_some(out)
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
            vec![Parameter::new(ParameterId::AUTH_DATA, challenge)],
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

    pub fn device_information_update(name: &str) -> Self {
        Self::new(
            ControlMessageId::DeviceInformationUpdate,
            vec![Parameter::new(ParameterId::DEVICE_NAME, name)],
        )
    }

    /// The 0x5702 dialect: no parameters. What the AUTO BOX bench unit answers (with a flat
    /// 0x5703 carrying SSID in parameter 1 and passphrase in parameter 2).
    pub fn request_accessory_wifi_config() -> Self {
        Self::new(
            ControlMessageId::RequestAccessoryWiFiConfigurationInformation,
            vec![],
        )
    }

    pub fn request_accessory_wifi_config_grouped(
        transport_identifier: &str,
        wifi_mac: &str,
    ) -> Self {
        let transport = |id, identifier: &str| {
            Parameter::new(
                id,
                vec![
                    Parameter::new(ParameterId::TRANSPORT_KIND, 0x01u8),
                    Parameter::new(ParameterId::TRANSPORT_IDENTIFIER, identifier),
                ],
            )
        };
        Self::new(
            ControlMessageId::RequestAccessoryWiFiConfigurationInformationGrouped,
            vec![
                transport(ParameterId::REQUEST_TRANSPORT_GROUP, transport_identifier),
                transport(ParameterId::REQUEST_WIFI_GROUP, wifi_mac),
            ],
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

#[cfg(test)]
mod tests {
    use super::*;

    fn unhex(s: &str) -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    /// Strip the 9-byte iAP2 packet header and the trailing payload checksum, leaving the
    /// control message as it appeared on the wire.
    fn payload(packet_hex: &str) -> Vec<u8> {
        let bytes = unhex(packet_hex);
        bytes[9..bytes.len() - 1].to_vec()
    }

    // Reference packets from `iphone-cfmoto.pklg`: a working CarPlay session between an
    // iPhone and the CFMOTO head unit. Encoding against these is the only check available
    // for the post-authentication sequence without a head unit on the bench.
    //
    const IPHONE_DEVICE_INFO: &str =
        "ff5a001f4046780288404000154e09000f0000546573744465766963650004";
    const IPHONE_WIFI_REQUEST: &str = "ff5a005540477802514040004b4300002600000005000001001d0001303132333435363738394142434445463031323334353637\
00001f000100050000010016000130323a30303a30303a30303a30303a303100aa";
    const CAR_WIFI_CONFIG: &str = "ff5a00ae40794702f7404000a44301004b00010012000043464d4f544f2d39393937303000000d00013132333435363738000005\
00029d001e0003666538303a3a393038333a386566663a666565663a613764330000050004030008000200001b58000c00053437352e382e3200001600033344\
3a32303a33443a44333a45423a3036000029000439613031316639392d353937652d343234642d623439302d3830366664376330373830300059";
    const IPHONE_AUTH_CHALLENGE: &str = "ff5a003440447702764040002aaa0200240000295edc4346f01a343d579fdff9562f18662a4ad76eeb0059ee81b10f16c682665e";

    #[test]
    fn device_information_update_matches_iphone() {
        let msg = ControlMessage::device_information_update("TestDevice");
        assert_eq!(msg.to_bytes().unwrap(), payload(IPHONE_DEVICE_INFO));
    }

    #[test]
    fn wifi_config_request_matches_iphone() {
        let msg = ControlMessage::request_accessory_wifi_config_grouped(
            "0123456789ABCDEF01234567",
            "02:00:00:00:00:01",
        );
        assert_eq!(msg.to_bytes().unwrap(), payload(IPHONE_WIFI_REQUEST));
    }

    /// The credentials sit in a nested group, not at the top level - reading them flat
    /// yields an empty SSID rather than an error, so this is worth pinning down.
    #[test]
    fn wifi_config_response_parses() {
        let bytes = payload(CAR_WIFI_CONFIG);
        let (_, msg) = ControlMessage::from_bytes((&bytes, 0)).unwrap();
        assert_eq!(
            msg.message_id,
            ControlMessageId::AccessoryWiFiConfigurationInformationGrouped
        );

        let group = msg
            .parameters
            .get_param::<Vec<Parameter>>(ParameterId::WIFI_CONFIG_GROUP)
            .expect("credentials group");
        assert_eq!(
            group.get_param::<String>(ParameterId::WIFI_CONFIG_SSID),
            Some(String::from("CFMOTO-999700"))
        );
        assert_eq!(
            group.get_param::<String>(ParameterId::WIFI_CONFIG_PASSPHRASE),
            Some(String::from("12345678"))
        );
        assert_eq!(
            group.get_param::<u8>(ParameterId::WIFI_CONFIG_CHANNEL),
            Some(157)
        );
        // The address of the AirPlay receiver, which is what we actually connect out to -
        // this unit never invites us, so discovery alone leaves both sides waiting.
        assert_eq!(
            group.get_param::<String>(ParameterId::WIFI_CONFIG_IPV6),
            Some(String::from("fe80::9083:8eff:feef:a7d3"))
        );

        assert_eq!(
            msg.parameters
                .get_param::<u32>(ParameterId::WIFI_CONFIG_PORT),
            Some(7000)
        );
        assert_eq!(
            msg.parameters
                .get_param::<String>(ParameterId::WIFI_CONFIG_MAC),
            Some(String::from("3D:20:3D:D3:EB:06"))
        );
    }

    #[test]
    fn auth_challenge_is_32_bytes() {
        let reference = payload(IPHONE_AUTH_CHALLENGE);
        let msg = ControlMessage::authentication_challenge(&[0u8; 32]);
        let encoded = msg.to_bytes().unwrap();
        assert_eq!(encoded.len(), reference.len());
        // Same header and parameter framing; only the random challenge itself differs.
        assert_eq!(encoded[..10], reference[..10]);
    }

    /// The AUTO BOX bench unit's 0x5703, rebuilt from the parameter bytes logged verbatim in
    /// `carjack.log` (2026-07-17). Flat parameters, no group - the opposite of the bike's
    /// 0x4301 above, which is why both dialects have to be supported.
    #[test]
    fn legacy_wifi_config_response_parses() {
        let bytes = unhex(
            "404000335703\
             0016000163617270 6c61795f776966695f3939303000\
             000d0002313233343536373800\
             0005000302\
             0005000424"
                .replace(' ', "")
                .as_str(),
        );
        let (_, msg) = ControlMessage::from_bytes((&bytes, 0)).unwrap();
        assert_eq!(
            msg.message_id,
            ControlMessageId::AccessoryWiFiConfigurationInformation
        );
        assert_eq!(
            msg.parameters.get_param::<String>(ParameterId::SSID),
            Some(String::from("carplay_wifi_9900"))
        );
        assert_eq!(
            msg.parameters.get_param::<String>(ParameterId::PASSPHRASE),
            Some(String::from("12345678"))
        );
        // The flat layout must NOT be read as a group, or the SSID comes back empty.
        assert!(
            msg.parameters
                .get_param::<Vec<Parameter>>(ParameterId::WIFI_CONFIG_GROUP)
                .is_none()
        );
    }

    /// The two dialects must stay distinct opcodes, since one accessory answers each.
    #[test]
    fn wifi_config_dialects_are_distinct() {
        assert_eq!(
            ControlMessage::request_accessory_wifi_config().to_bytes().unwrap(),
            unhex("404000065702")
        );
        let grouped = ControlMessage::request_accessory_wifi_config_grouped("a", "b");
        assert_eq!(grouped.message_id.deku_id().unwrap(), 0x4300);
    }

    #[test]
    fn plain_value_is_not_mistaken_for_a_group() {
        let params = vec![Parameter::new(ParameterId::DEVICE_NAME, "TestDevice")];
        assert!(
            params
                .get_param::<Vec<Parameter>>(ParameterId::DEVICE_NAME)
                .is_none()
        );
    }

    /// Strings are written with their NUL terminator and read back without it.
    #[test]
    fn string_parameter_round_trips() {
        let params = vec![Parameter::new(ParameterId::DEVICE_NAME, "TestDevice")];
        assert_eq!(params[0].data.last(), Some(&0));
        assert_eq!(
            params.get_param::<String>(ParameterId::DEVICE_NAME),
            Some(String::from("TestDevice"))
        );
    }
}
