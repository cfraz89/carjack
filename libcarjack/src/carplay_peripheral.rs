use crate::usb_schema::{
    AudioStreamingEndpoints, CarplayConfigurations, CarplayPeripheral, Configuration, DeviceClass,
    Endpoint, EndpointAddress, EndpointDirection, EndpointType, HidEndpoints, Id, Interface,
    IpodUsbInterfaces, PtpEndpoints, PtpInterfaces, Strings, SynchronizationType, UsageType,
};

pub const VENDOR_APPLE: u16 = 0x05ac;
pub const PRODUCT_IPHONE: u16 = 0x12a8;

pub const CARJACK_PTP_INTERFACES: PtpInterfaces = PtpInterfaces {
    ptp: Interface {
        description: Some("PTP"),
        class: usb::class_code::STILL_IMAGING,
        sub_class: 0x01, //Still Image Capture
        protocol: 0x01,  //Picture Transfer Protocol (PIMA 15470)
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
    device_class: DeviceClass {
        class: usb::class_code::DEVICE,
        sub_class: usb::class_code::DEVICE,
        protocol: usb::class_code::DEVICE,
    },
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
            attributes: 0xc0,
            interfaces: CARJACK_PTP_INTERFACES,
        },
        ipod_usb: Configuration {
            description: "iPod USB Configuration",
            attributes: 0xc0,
            interfaces: IpodUsbInterfaces {
                audio_control: Interface {
                    description: None,
                    class: usb::class_code::AUDIO,
                    sub_class: 0x01, //Audio Control
                    protocol: 0x00,  //Unspecified
                    endpoints: (),
                },
                audio_streaming: Interface {
                    description: None,
                    class: usb::class_code::AUDIO,
                    sub_class: 0x02, //Audio Streaming
                    protocol: 0x00,  //Unspecified
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
                    description: Some("HID Interface"),
                    class: usb::class_code::HID,
                    sub_class: 0x00, //No Subclass
                    protocol: 0x00,  //Unspecified
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
            attributes: 0xc0,
            interfaces: CARJACK_PTP_INTERFACES,
        },
        ptp_and_apple_mobile_device_and_apple_usb_ethernet: Configuration {
            description: "PTP, Apple Mobile Device, and Apple USB Ethernet Configuration",
            attributes: 0xc0,
            interfaces: CARJACK_PTP_INTERFACES,
        },
    },
};
