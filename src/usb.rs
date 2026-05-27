use color_eyre::Result;
use eyre::eyre;
use tracing::debug;
use usb_gadget::{
    Class, Config, Gadget, Id, RegGadget, Strings,
    function::{
        Handle,
        custom::{
            Custom, Endpoint, EndpointDirection, EndpointReceiver, EndpointSender, Interface,
            TransferType,
        },
        net::{Net, NetClass},
    },
    udcs,
};

/// USB class of PTP:
/// bInterfaceClass         6 Imaging
/// bInterfaceSubClass      1 Still Image Capture
/// bInterfaceProtocol      1 Picture Transfer Protocol (PIMA 15470)
const PTP_PIMA_15470: Class = Class::new(0x06, 0x01, 0x01);

/// Register this system as a carplay-compatible usb device
pub fn register_gadget() -> Result<RegGadget> {
    let ptp_fn = PtpFunction::new();
    let ncm_fn = Net::new(NetClass::Ncm);

    let gadget = Gadget::new(
        Class::INTERFACE_SPECIFIC,
        Id::new(0x05ac, 0x12a8),
        Strings::new("Apple Inc.", "iPhone", "989cad3f25b0"),
    )
    .with_config(Config::new("PTP + Apple Mobile Device").with_function(ptp_fn.handle.clone()))
    .with_config(
        Config::new("PTP + Apple Mobile Device + Apple USB Ethernet")
            .with_function(ptp_fn.handle)
            .with_function(ncm_fn.1),
    );

    let udcs = udcs()?;
    debug!(?udcs);

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
