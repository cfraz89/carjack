use crate::usb_schema::{
    AudioStreamingEndpoints, CarplayConfigurations, CarplayPeripheral, Class, Configuration,
    ConfigurationAttributes, Endpoint, EndpointAddress, EndpointDirection, EndpointType,
    HidEndpoints, Id, Interface, IpodUsbInterfaces, PtpEndpoints, PtpInterfaces, Strings,
    SynchronizationType, UsageType,
};

pub const VENDOR_APPLE: u16 = 0x05ac;
pub const PRODUCT_IPHONE: u16 = 0x12a8;

pub const CARJACK_PTP_INTERFACES: PtpInterfaces = PtpInterfaces {
    ptp: Interface {
        description: "PTP",
        class: Class::new(
            usb::class_code::STILL_IMAGING,
            0x01, /* Still Image Capture */
            0x01, /* Picture Transfer Protocol (PIMA 15470) */
        ),
        endpoints: PtpEndpoints {
            bulk_out: Endpoint {
                address: EndpointAddress::new(1, EndpointDirection::Out),
                endpoint_type: EndpointType::Bulk,
                max_packet_size: 0x0200,
                interval: 0,
            },
            bulk_in: Endpoint {
                address: EndpointAddress::new(1, EndpointDirection::In),
                endpoint_type: EndpointType::Bulk,
                max_packet_size: 0x0200,
                interval: 0,
            },
            int_in: Endpoint {
                address: EndpointAddress::new(3, EndpointDirection::In),
                endpoint_type: EndpointType::Interrupt,
                max_packet_size: 0x0040,
                interval: 6,
            },
        },
    },
};

pub const CARJACK_PERIPHERAL: CarplayPeripheral = CarplayPeripheral {
    device_class: Class::new(usb::class_code::DEVICE, 0x00f, 0x00f),
    id: Id {
        vendor_id: VENDOR_APPLE,
        product_id: PRODUCT_IPHONE,
    },
    strings: Strings {
        manufacturer: "Apple Inc.",
        product: "iPhone",
        serial_number: "989cad3f25b0",
    },
    configurations: CarplayConfigurations {
        ptp: Configuration {
            description: "PTP",
            attributes: ConfigurationAttributes::new(true, false),
            max_power: 500,
            interfaces: CARJACK_PTP_INTERFACES,
        },
        ipod_usb: Configuration {
            description: "iPod USB Configuration",
            attributes: ConfigurationAttributes::new(true, false),
            max_power: 500,
            interfaces: IpodUsbInterfaces {
                audio_control: Interface {
                    description: "",
                    class: Class::new(
                        usb::class_code::AUDIO,
                        0x01, /* Audio Control */
                        0x00, /* Unspecified */
                    ),
                    endpoints: (),
                },
                audio_streaming: Interface {
                    description: "",
                    class: Class::new(
                        usb::class_code::AUDIO,
                        0x02, /* Audio Streaming */
                        0x00, /* Unspecified */
                    ),
                    endpoints: AudioStreamingEndpoints {
                        iso_in: Endpoint {
                            address: EndpointAddress::new(4, EndpointDirection::In),
                            endpoint_type: EndpointType::Isochronous(
                                SynchronizationType::NoSynchronization,
                                UsageType::DataEndpoint,
                            ),
                            max_packet_size: 0x0100,
                            interval: 4,
                        },
                    },
                },
                hid: Interface {
                    description: "HID Interface",
                    class: Class::new(usb::class_code::HID, 0x00, 0x00),
                    endpoints: HidEndpoints {
                        int_in: Endpoint {
                            address: EndpointAddress::new(3, EndpointDirection::In),
                            endpoint_type: EndpointType::Interrupt,
                            max_packet_size: 0x0040,
                            interval: 4,
                        },
                    },
                },
            },
        },
        ptp_and_apple_mobile_device: Configuration {
            description: "PTP + Apple Mobile Device",
            attributes: ConfigurationAttributes::new(true, false),
            max_power: 500,
            interfaces: CARJACK_PTP_INTERFACES,
        },
        ptp_and_apple_mobile_device_and_apple_usb_ethernet: Configuration {
            description: "PTP, Apple Mobile Device, and Apple USB Ethernet Configuration",
            attributes: ConfigurationAttributes::new(true, false),
            max_power: 500,
            interfaces: CARJACK_PTP_INTERFACES,
        },
    },
};
