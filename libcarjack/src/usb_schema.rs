pub struct DeviceClass {
    pub class: u8,
    pub sub_class: u8,
    pub protocol: u8,
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

pub struct Configuration {
    pub description: &'static str,
    pub attributes: u8,
    pub interfaces: Vec<Interface>,
}

pub struct Interface {
    pub class: u8,
    pub sub_class: u8,
    pub protocol: u8,
    pub endpoints: Vec<Endpoint>,
}

pub struct Endpoint {
    pub number: u8,
    pub direction: EndpointDirection,
    pub attributes: EndpointType,
    pub max_packet_size: u16,
    pub interval: u8,
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

pub struct CarplayConfigurations {
    pub ptp: Configuration,
    pub ipod_usb: Configuration,
    pub ptp_and_apple_mobile_device: Configuration,
    pub ptp_and_apple_mobile_device_and_apple_usb_ethernet: Configuration,
}
pub struct CarplayPeripheral {
    pub device_class: DeviceClass,
    pub id: Id,
    pub strings: Strings,
    pub configurations: CarplayConfigurations,
}
