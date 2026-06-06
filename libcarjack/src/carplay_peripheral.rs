use crate::usb_schema::{
    CarplayConfigurations, CarplayPeripheral, Configuration, DeviceClass, Endpoint, Id, Interface,
    Strings,
};

pub const VENDOR_APPLE: u16 = 0x05ac;
pub const PRODUCT_IPHONE: u16 = 0x12a8;

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
            interfaces: vec![Interface {
                class: usb::class_code::STILL_IMAGING,
                sub_class: 0x01, //Still Image Capture
                protocol: 0x01,  //Picture Transfer Protocol (PIMA 15470)
                endpoints: vec![],
            }],
        },
        ipod_usb: Configuration {
            description: "iPod USB Configuration",
            attributes: 0xc0,
        },
        ptp_and_apple_mobile_device: Configuration {
            description: "PTP and Apple Mobile Device Configuration",
            attributes: 0xc0,
        },
        ptp_and_apple_mobile_device_and_apple_usb_ethernet: Configuration {
            description: "PTP, Apple Mobile Device, and Apple USB Ethernet Configuration",
            attributes: 0xc0,
        },
    },
};
