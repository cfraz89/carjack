use color_eyre::Result;
use eyre::eyre;
use libcarjack::{carplay_peripheral::CARJACK_PERIPHERAL, usb_schema};
use usb_gadget::{
    Class, Config, Gadget, Id, RegGadget, Strings,
    function::{
        Handle,
        custom::{
            Custom, Endpoint, EndpointDirection, EndpointReceiver, EndpointSender, Interface,
            TransferType,
        },
        hid::Hid,
        net::{Net, NetClass},
    },
    udcs,
};

fn into_gadget_class(device_class: usb_schema::Class) -> Class {
    Class::new(
        device_class.class,
        device_class.sub_class,
        device_class.protocol,
    )
}

fn into_gadget_interface<E>(interface: usb_schema::Interface<E>) -> Interface {
    Interface::new(into_gadget_class(interface.class), interface.description)
}

fn into_gadget_id(id: usb_schema::Id) -> Id {
    Id::new(id.vendor_id, id.product_id)
}

fn into_gadget_strings(strings: usb_schema::Strings) -> Strings {
    Strings::new(strings.manufacturer, strings.product, strings.serial_number)
}

fn into_gadget_configuration<E>(configuration: usb_schema::Configuration<E>) -> Config {
    let mut config = Config::new(configuration.description);
    config.max_power = configuration.max_power;
    config.self_powered = configuration.attributes.self_powered;
    config.remote_wakeup = configuration.attributes.remote_wakeup;
    config
}

/// USB class of PTP:
/// bInterfaceClass         6 Imaging
/// bInterfaceSubClass      1 Still Image Capture
/// bInterfaceProtocol      1 Picture Transfer Protocol (PIMA 15470)
const PTP_PIMA_15470: Class = Class::new(0x06, 0x01, 0x01);

/// Register this system as a carplay-compatible usb device
pub fn register_gadget() -> Result<RegGadget> {
    let ptp_fn = PtpFunction::new();

    let gadget = Gadget::new(
        into_gadget_class(CARJACK_PERIPHERAL.device_class),
        into_gadget_id(CARJACK_PERIPHERAL.id),
        into_gadget_strings(CARJACK_PERIPHERAL.strings),
    )
    .with_config(
        into_gadget_configuration(CARJACK_PERIPHERAL.configurations.ptp)
            .with_function(ptp_fn.handle.clone()),
    )
    .with_config(
        into_gadget_configuration(CARJACK_PERIPHERAL.configurations.ipod_usb)
            .with_function(AudioControlFunction::new().handle)
            .with_function(AudioStreamingFunction::new().handle)
            .with_function(HidFunction::new().handle),
    )
    .with_config(
        into_gadget_configuration(
            CARJACK_PERIPHERAL
                .configurations
                .ptp_and_apple_mobile_device,
        )
        .with_function(ptp_fn.handle.clone()),
    )
    .with_config(
        into_gadget_configuration(
            CARJACK_PERIPHERAL
                .configurations
                .ptp_and_apple_mobile_device_and_apple_usb_ethernet,
        )
        .with_function(ptp_fn.handle.clone()),
    );

    let udcs = udcs()?;

    let reg_gadget = gadget.bind(udcs.first().ok_or(eyre!("No udcs on system"))?)?;

    Ok(reg_gadget)
}

/// Contains handles to interfaces within the PTP function, as well as the function itself
struct PtpFunction {
    pub bulk_out: EndpointReceiver,
    pub bulk_in: EndpointSender,
    pub int_in: EndpointSender,
    pub function: Custom,
    pub handle: Handle,
}

impl PtpFunction {
    fn new() -> Self {
        let (bulk_out, bulk_out_dir) = EndpointDirection::host_to_device();
        let (bulk_in, bulk_in_dir) = EndpointDirection::device_to_host();
        let (int_in, int_in_dir) = EndpointDirection::device_to_host();
        let (function, handle) = Custom::builder()
            .with_interface(
                Interface::new(PTP_PIMA_15470, "PTP")
                    .with_endpoint(Endpoint::bulk(bulk_out_dir))
                    .with_endpoint(Endpoint::bulk(bulk_in_dir))
                    .with_endpoint(Endpoint::custom(int_in_dir, TransferType::Interrupt)),
            )
            .build();
        PtpFunction {
            bulk_out,
            bulk_in,
            int_in,
            function,
            handle,
        }
    }
}

struct AudioControlFunction {
    pub function: Custom,
    pub handle: Handle,
}

impl AudioControlFunction {
    fn new() -> Self {
        let (function, handle) = Custom::builder()
            .with_interface(into_gadget_interface(
                CARJACK_PERIPHERAL
                    .configurations
                    .ipod_usb
                    .interfaces
                    .audio_control,
            ))
            .build();
        AudioControlFunction { function, handle }
    }
}

struct AudioStreamingFunction {
    pub function: Custom,
    pub handle: Handle,
}

impl AudioStreamingFunction {
    fn new() -> Self {
        let (function, handle) = Custom::builder()
            .with_interface(into_gadget_interface(
                CARJACK_PERIPHERAL
                    .configurations
                    .ipod_usb
                    .interfaces
                    .audio_streaming,
            ))
            .build();
        AudioStreamingFunction { function, handle }
    }
}

struct HidFunction {
    pub function: Hid,
    pub handle: Handle,
}

impl HidFunction {
    fn new() -> Self {
        let hid_class = CARJACK_PERIPHERAL
            .configurations
            .ipod_usb
            .interfaces
            .hid
            .class;
        let mut builder = Hid::builder();
        builder.sub_class = hid_class.sub_class;
        builder.protocol = hid_class.protocol;
        let (function, handle) = builder.build();
        HidFunction { function, handle }
    }
}
