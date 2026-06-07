pub struct Class {
    pub class: u8,
    pub sub_class: u8,
    pub protocol: u8,
}

impl Class {
    pub const fn new(class: u8, sub_class: u8, protocol: u8) -> Self {
        Class {
            class,
            sub_class,
            protocol,
        }
    }
}

pub struct Id {
    pub vendor_id: u16,
    pub product_id: u16,
}

pub struct Strings {
    pub manufacturer: &'static str,
    pub product: &'static str,
    pub serial_number: &'static str,
}

pub struct Configuration<I> {
    pub description: &'static str,
    pub attributes: ConfigurationAttributes,
    pub max_power: u16, // in mA
    pub interfaces: I,
}

pub struct ConfigurationAttributes {
    pub self_powered: bool,
    pub remote_wakeup: bool,
}

impl ConfigurationAttributes {
    pub const fn new(self_powered: bool, remote_wakeup: bool) -> Self {
        ConfigurationAttributes {
            self_powered,
            remote_wakeup,
        }
    }
}

pub struct Interface<E> {
    pub class: Class,
    pub description: Option<&'static str>,
    pub endpoints: E,
    //Maybe todo, specific interface descriptors, eg AudioControl, AudioStreaming, etc
}

pub struct Endpoint {
    pub address: EndpointAddress,
    pub endpoint_type: EndpointType,
    pub max_packet_size: u16,
    pub interval: u8,
}

pub struct EndpointAddress {
    pub number: u8,
    pub direction: EndpointDirection,
}

impl EndpointAddress {
    pub const fn new(number: u8, direction: EndpointDirection) -> Self {
        EndpointAddress { number, direction }
    }
}

impl Into<u8> for EndpointAddress {
    fn into(self) -> u8 {
        let direction_bit = match self.direction {
            EndpointDirection::Out => 0,
            EndpointDirection::In => 0x80,
        };
        direction_bit | (self.number & 0x0F)
    }
}

pub enum EndpointType {
    Control,
    Isochronous(SynchronizationType, UsageType),
    Bulk,
    Interrupt,
}

pub enum SynchronizationType {
    NoSynchronization,
    Asynchronous,
    Adaptive,
    Synchronous,
}

pub enum UsageType {
    DataEndpoint,
    FeedbackEndpoint,
    ExplicitFeedbackDataEndpoint,
    Reserved,
}

pub enum EndpointDirection {
    Out,
    In,
}

pub enum TransferType {
    Control,
    Isochronous,
    Bulk,
    Interrupt,
}

pub struct PtpInterfaces {
    pub ptp: Interface<PtpEndpoints>,
}

pub struct IpodUsbInterfaces {
    pub audio_control: Interface<()>,
    pub audio_streaming: Interface<AudioStreamingEndpoints>,
    pub hid: Interface<HidEndpoints>,
}

pub struct PtpEndpoints {
    pub bulk_out: Endpoint,
    pub bulk_in: Endpoint,
    pub int_in: Endpoint,
}

pub struct AudioStreamingEndpoints {
    pub iso_in: Endpoint,
}

pub struct HidEndpoints {
    pub int_in: Endpoint,
}

pub struct CarplayConfigurations {
    pub ptp: Configuration<PtpInterfaces>,
    pub ipod_usb: Configuration<IpodUsbInterfaces>,
    pub ptp_and_apple_mobile_device: Configuration<PtpInterfaces>,
    pub ptp_and_apple_mobile_device_and_apple_usb_ethernet: Configuration<PtpInterfaces>,
}
pub struct CarplayPeripheral {
    pub device_class: Class,
    pub id: Id,
    pub strings: Strings,
    pub configurations: CarplayConfigurations,
}
