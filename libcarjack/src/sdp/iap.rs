use btuuid::browse_group_identifiers::PUBLIC_BROWSE_ROOT;
use btuuid::protocol_identifiers::{L2_CAP, RFCOMM};
use btuuid::service_class::SERIAL_PORT;

use crate::sdp::types::{attribute, SdpRecord, FULLY_AVAILABLE, SERIAL_PORT_VERSION_1_2};
use crate::sdp::types::{SdpAttributeId::*, ENCODING_UTF8, LANGUAGE_EN};

/// The iAP record an iPhone publishes, field for field.
/// We're not using the default bluez-generated record
/// because our test bike's head unit probes for the Serial Port profile descriptor,
/// which we can't specify through the bluez api.
pub fn iap_service_record(iap_channel: u8) -> SdpRecord {
    SdpRecord {
        attribute: vec![
            attribute(ServiceClassIdList, (crate::uuid::IAP_DEVICE_UUID,)),
            attribute(ProtocolDescriptorList, ((L2_CAP,), (RFCOMM, iap_channel))),
            attribute(BrowseGroupList, (PUBLIC_BROWSE_ROOT,)),
            attribute(
                LanguageBaseAttributeIdList,
                (LANGUAGE_EN, ENCODING_UTF8, ServiceName as u16),
            ),
            attribute(ServiceAvailability, FULLY_AVAILABLE),
            attribute(
                BluetoothProfileDescriptorList,
                ((SERIAL_PORT, SERIAL_PORT_VERSION_1_2),),
            ),
            attribute(ServiceName, "Wireless iAP"),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::iap_service_record;

    /// The record serializes every attribute under its `id`, and still carries the Serial Port
    /// descriptor a head unit probes for, plus the device UUID.
    #[test]
    fn service_record_carries_the_serial_port_descriptor() {
        let record = iap_service_record(1).to_xml();
        assert!(record.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\" ?>\n"));
        for id in [
            "0x0001", "0x0004", "0x0005", "0x0006", "0x0008", "0x0009", "0x0100",
        ] {
            assert!(
                record.contains(&format!("<attribute id=\"{id}\">")),
                "missing attribute {id}"
            );
        }
        for fragment in [
            &crate::uuid::IAP_DEVICE_UUID.to_string(),
            "<uint8 value=\"0x01\"/>",
            "<uuid value=\"0x1002\"/>",
            "<uint16 value=\"0x656e\"/>",
            "<uint16 value=\"0x006a\"/>",
            "<uint16 value=\"0x0100\"/>",
            "<uint8 value=\"0xff\"/>",
            "<uuid value=\"0x1101\"/>",
            "<uint16 value=\"0x0102\"/>",
            "<text value=\"Wireless iAP\"/>",
        ] {
            assert!(record.contains(fragment), "missing {fragment}");
        }
    }
}
